"""Draw an interactive overlay panel and pump its input lane until it is dismissed.

Unlike examples 01-05 this presents a vector display list rather than a raster: the host
shapes the text and rasterizes the paths, so moving the window never reuploads anything.
"""
import time

from _support import arguments
from vivid_sdk import OverlaySession, OverlayWindowOptions
from vivid_sdk.overlay import (
    Brush,
    Canvas,
    ConnectionLostEvent,
    DismissedEvent,
    Path,
    Point,
    PointerEvent,
    Rect,
)

WIDTH, HEIGHT = 320.0, 180.0
# Application-chosen hit region ID. Nonzero and unique within one display list.
PANEL = 1


def panel(label: str) -> Canvas:
    face = Path.rounded_rectangle(Rect(0, 0, WIDTH, HEIGHT), 12)
    return (
        Canvas()
        .fill(face, Brush.solid(0x203050FF))
        .stroke(face, Brush.solid(0x66CCFFFF), 2)
        # An empty family asks the host for its default; custom font bytes are not supported.
        .text(label, Point(20, 24), 18, 0xFFFFFFFF)
        # Declaring a region lets the host report which part was pressed; the default hit area
        # is the whole window rectangle.
        .hit(PANEL, face)
    )


def main() -> None:
    options = arguments(__doc__.splitlines()[0])
    deadline = None if options.duration is None else time.monotonic() + options.duration
    with OverlaySession.from_env() as session:
        with session.create_window(OverlayWindowOptions(Rect(40, 40, WIDTH, HEIGHT))) as window:
            window.present(panel("Click the panel, or press Escape."))
            window.center()
            window.request_focus()
            while deadline is None or time.monotonic() < deadline:
                event = session.wait_event(0.2)
                if event is None:
                    continue
                if isinstance(event, ConnectionLostEvent):
                    print(f"overlay connection lost: {event.diagnostic}")
                    break
                # One session can own many windows, so every event names the one it belongs to.
                if not event.targets(window):
                    continue
                if isinstance(event, PointerEvent) and event.application_id == PANEL and event.down:
                    break
                if isinstance(event, DismissedEvent):
                    break


if __name__ == "__main__":
    main()
