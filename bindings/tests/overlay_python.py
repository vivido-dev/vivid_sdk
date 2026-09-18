"""Producer side of Vivido's bounded native binding integration test (not an example)."""
import asyncio
import sys
import time
from vivid_sdk import OverlaySession, OverlayWindowOptions, aio
from vivid_sdk.overlay import Canvas, Brush, Path, Rect, Point, ImeEvent, PointerEvent, ViewportEvent, StyledText, TextRun, TextStyle

OPTIONS = OverlayWindowOptions(Rect(10, 20, 100, 80))
PIXELS = bytes([255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255])
TEXT = StyledText((TextRun("A😀", TextStyle(size=18, color=0xFF0000FF, underline=True)), TextRun("日", TextStyle(size=20, color=0x0000FFFF, strikethrough=True))), max_width=90, alignment="center")
ELLIPSIS = StyledText((TextRun("A😀日e\u0301 long label to truncate", TextStyle(size=12, color=0x00FF00FF)),), max_width=85, max_lines=1, overflow="ellipsis", letter_spacing=0.5, word_spacing=2, line_height=16, ligatures=False, kerning=False)

def check_ellipsis(measured):
    cut = measured.truncated_at
    assert cut is not None and 0 < cut < len(ELLIPSIS.runs[0].text)
    assert ELLIPSIS.runs[0].text[cut] != "\u0301"
    assert all(c.end <= cut for c in measured.clusters)
    assert any(c.start == c.end == cut and c.bounds.width > 0 for c in measured.clusters)

def canvas():
    result = Canvas()
    for x, y, color in [(0, 0, 0xff0000ff), (10, 0, 0x00ff00ff), (0, 10, 0x0000ffff), (10, 10, 0xffff00ff)]:
        result.fill(Path.rectangle(Rect(x, y, 10, 10)), Brush.solid(color))
    return result.hit((1 << 63) + 17, Path.rectangle(Rect(0, 0, 100, 80)))

def accept(event, window, seen):
    if isinstance(event, ViewportEvent) and event.viewport.width == 500:
        assert event.revision >= 2 and event.scene_revision == 0
        seen.add("viewport")
    if isinstance(event, (PointerEvent, ImeEvent)):
        assert event.targets(window) and event.scene_revision == 1
    if isinstance(event, PointerEvent):
        assert event.application_id == (1 << 63) + 17
        seen.add("pointer")
    if isinstance(event, ImeEvent):
        assert event.preedit == "A😀日" and event.selection == (1, 2)
        seen.add("ime")

def blocking():
    with OverlaySession.from_env() as session:
        with session.create_window(OPTIONS) as window:
            window.center(); window.set_bounds(OPTIONS.bounds)
            window.set_visible(False); window.set_visible(True)
            window.raise_window(); window.lower(); window.request_focus()
            assert window.bounds() == OPTIONS.bounds
            assert window.viewport().width == 400
            measured = window.measure_text("A😀日", 18)
            batch = window.measure_text_batch((TEXT, TEXT))
            layout = window.layout_text_batch((TEXT,))[0]
            ellipsis = window.layout_text(ELLIPSIS)
            check_ellipsis(ellipsis.measurement)
            assert window.measure_text_batch((ELLIPSIS,))[0] == ellipsis.measurement
            assert batch[0] == batch[1] == layout.measurement
            assert max(c.end for c in layout.measurement.clusters) == 3
            assert measured.width > 0 and measured.height > 0 and measured.lines
            assert max(c.end for c in measured.clusters) == 3
            scene = canvas()
            window.draw_text_layout(scene, layout, Point(0, 40))
            window.draw_text_layout(scene, ellipsis, Point(0, 22))
            image = window.upload_rgba(2, 2, PIXELS)
            window.draw_image(scene, image, Rect(50, 0, 20, 20))
            receipt = window.submit(scene)
            assert receipt.revision == 1 and receipt.wait(10) == "presented"
            window.release_image(image)
            try: window.submit(scene)
            except (ValueError, OSError): pass
            else: raise AssertionError("released image was accepted")
            seen = set(); deadline = time.monotonic() + 10
            for event in session.events():
                accept(event, window, seen)
                assert time.monotonic() < deadline
                if len(seen) == 3: break
            session.capture_pointer(window); session.capture_pointer(window, False)
            window.set_editor_geometry(receipt.revision, Rect(5, 6, 1, 18))
            window.set_editor_geometry(receipt.revision, None)
            assert sys.stdin.readline().strip() == "release"
            pending = window.submit(Canvas())
            replacement_scene = canvas()
            window.draw_text_layout(replacement_scene, layout, Point(0, 40))
            window.draw_text_layout(replacement_scene, ellipsis, Point(0, 22))
            replacement = window.replace_track(replacement_scene)
            assert pending.wait(5) == "superseded" and replacement.revision == 3
            status = window.reconcile()
            assert status.active_revision == status.accepted_revision == 3
            assert replacement.wait(10) == "presented"
            assert window.reconcile().presented_revision == 3
            window.release_text_layout(layout)
            window.release_text_layout(ellipsis)
            try: window.submit(replacement_scene)
            except (ValueError, OSError): pass
            else: raise AssertionError("released layout was accepted")
            popup = session.create_window(OverlayWindowOptions(Rect(40, 40, 20, 20), "popup"), parent=window)
            popup.present(Canvas()); popup.close()
            window.present(canvas())

async def asynchronous():
    async with await aio.OverlaySession.from_env() as session:
        async with await session.create_window(OPTIONS) as window:
            await window.center(); await window.set_bounds(OPTIONS.bounds)
            await window.set_visible(False); await window.set_visible(True)
            await window.raise_window(); await window.lower(); await window.request_focus()
            assert await window.bounds() == OPTIONS.bounds
            assert (await window.viewport()).width == 400
            measured = await window.measure_text("A😀日", 18)
            batch = await window.measure_text_batch((TEXT, TEXT))
            layout = (await window.layout_text_batch((TEXT,)))[0]
            ellipsis = await window.layout_text(ELLIPSIS)
            check_ellipsis(ellipsis.measurement)
            assert (await window.measure_text_batch((ELLIPSIS,)))[0] == ellipsis.measurement
            assert batch[0] == batch[1] == layout.measurement
            assert max(c.end for c in layout.measurement.clusters) == 3
            assert measured.width > 0 and measured.height > 0 and measured.lines
            assert max(c.end for c in measured.clusters) == 3
            scene = canvas()
            await window.draw_text_layout(scene, layout, Point(0, 40))
            await window.draw_text_layout(scene, ellipsis, Point(0, 22))
            image = await window.upload_rgba(2, 2, PIXELS)
            await window.draw_image(scene, image, Rect(50, 0, 20, 20))
            receipt = await window.submit(scene)
            assert receipt.revision == 1 and await receipt.wait(10) == "presented"
            await window.release_image(image)
            try: await window.submit(scene)
            except (ValueError, OSError): pass
            else: raise AssertionError("released image was accepted")
            seen = set()
            async def receive():
                async for event in session.events():
                    accept(event, window, seen)
                    if len(seen) == 3: return
            await asyncio.wait_for(receive(), 10)
            await session.capture_pointer(window); await session.capture_pointer(window, False)
            await window.set_editor_geometry(receipt.revision, Rect(5, 6, 1, 18))
            await window.set_editor_geometry(receipt.revision, None)
            assert sys.stdin.readline().strip() == "release"
            pending = await window.submit(Canvas())
            replacement_scene = canvas()
            await window.draw_text_layout(replacement_scene, layout, Point(0, 40))
            await window.draw_text_layout(replacement_scene, ellipsis, Point(0, 22))
            replacement = await window.replace_track(replacement_scene)
            assert await pending.wait(5) == "superseded" and replacement.revision == 3
            status = await window.reconcile()
            assert status.active_revision == status.accepted_revision == 3
            assert await replacement.wait(10) == "presented"
            assert (await window.reconcile()).presented_revision == 3
            await window.release_text_layout(layout)
            await window.release_text_layout(ellipsis)
            try: await window.submit(replacement_scene)
            except (ValueError, OSError): pass
            else: raise AssertionError("released layout was accepted")
            popup = await session.create_window(OverlayWindowOptions(Rect(40, 40, 20, 20), "popup"), parent=window)
            await popup.present(Canvas()); await popup.close()
            await window.present(canvas())

if sys.argv[-1] == "async": asyncio.run(asynchronous())
else: blocking()
