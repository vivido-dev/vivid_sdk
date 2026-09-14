"""Producer side of Vivido's bounded native binding integration test (not an example)."""
import asyncio
import sys
import time
from vivid_sdk import OverlaySession, OverlayWindowOptions, aio
from vivid_sdk.overlay import Canvas, Brush, Path, Rect, ImeEvent, PointerEvent

OPTIONS = OverlayWindowOptions(Rect(10, 20, 100, 80))
PIXELS = bytes([255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255])

def canvas():
    result = Canvas()
    for x, y, color in [(0, 0, 0xff0000ff), (10, 0, 0x00ff00ff), (0, 10, 0x0000ffff), (10, 10, 0xffff00ff)]:
        result.fill(Path.rectangle(Rect(x, y, 10, 10)), Brush.solid(color))
    return result.hit((1 << 63) + 17, Path.rectangle(Rect(0, 0, 100, 80)))

def accept(event, window, seen):
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
            scene = canvas()
            image = window.upload_rgba(2, 2, PIXELS)
            window.draw_image(scene, image, Rect(50, 0, 20, 20))
            window.present(scene)
            seen = set(); deadline = time.monotonic() + 10
            for event in session.events():
                accept(event, window, seen)
                assert time.monotonic() < deadline
                if len(seen) == 2: break
            session.capture_pointer(window); session.capture_pointer(window, False)
            assert sys.stdin.readline().strip() == "release"
            popup = session.create_window(OverlayWindowOptions(Rect(40, 40, 20, 20), "popup"), parent=window)
            popup.present(Canvas()); popup.close()
            window.present(scene)

async def asynchronous():
    async with await aio.OverlaySession.from_env() as session:
        async with await session.create_window(OPTIONS) as window:
            await window.center(); await window.set_bounds(OPTIONS.bounds)
            await window.set_visible(False); await window.set_visible(True)
            await window.raise_window(); await window.lower(); await window.request_focus()
            assert await window.bounds() == OPTIONS.bounds
            assert (await window.viewport()).width == 400
            scene = canvas()
            image = await window.upload_rgba(2, 2, PIXELS)
            await window.draw_image(scene, image, Rect(50, 0, 20, 20))
            await window.present(scene)
            seen = set()
            async def receive():
                async for event in session.events():
                    accept(event, window, seen)
                    if len(seen) == 2: return
            await asyncio.wait_for(receive(), 10)
            await session.capture_pointer(window); await session.capture_pointer(window, False)
            assert sys.stdin.readline().strip() == "release"
            popup = await session.create_window(OverlayWindowOptions(Rect(40, 40, 20, 20), "popup"), parent=window)
            await popup.present(Canvas()); await popup.close()
            await window.present(scene)

if sys.argv[-1] == "async": asyncio.run(asynchronous())
else: blocking()
