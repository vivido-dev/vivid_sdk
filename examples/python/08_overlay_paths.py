"""Draw freeform paths: curves, an even-odd hole, and a stroke with caps, joins and dashes.

Example 07 used the shape constructors - rectangles, rounded rectangles, ellipses - and those
cover a user interface. Everything else is a path you build: a chart, a map, a signature, a
gauge. The four segment kinds compose into all of it, and the even-odd rule is what puts a hole
in a shape rather than a second layer over it.

The same builder exists in all three languages, with the same segment kinds and the same
options, so the geometry below is written the same way in each.
"""
import math
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
    StrokeStyle,
)

WIDTH, HEIGHT = 400.0, 220.0
# Application-chosen hit region ID. Nonzero and unique within one display list.
CANVAS = 1
# The circle constant, for the cubic approximation of a round shape.
KAPPA = 0.5522847498307936


def circle(path: Path, cx: float, cy: float, radius: float) -> Path:
    """A circle as four cubics, appended to a path that may already have subpaths.

    A ring needs two of these in one builder, and a finished Path cannot be extended - which is
    what a builder is for. Path.ellipse does the same arithmetic for a whole ellipse.
    """
    k = radius * KAPPA
    return (
        path.move_to(cx, cy - radius)
        .cubic_to(cx + k, cy - radius, cx + radius, cy - k, cx + radius, cy)
        .cubic_to(cx + radius, cy + k, cx + k, cy + radius, cx, cy + radius)
        .cubic_to(cx - k, cy + radius, cx - radius, cy + k, cx - radius, cy)
        .cubic_to(cx - radius, cy - k, cx - k, cy - radius, cx, cy - radius)
        .close()
    )


def star(cx: float, cy: float, radius: float) -> Path:
    """A star, filled: ten corners alternating between two radii, walked with line_to."""
    path = Path()
    for corner in range(10):
        reach = radius if corner % 2 == 0 else radius * 0.45
        angle = -math.pi / 2 + corner * math.pi / 5
        x, y = cx + reach * math.cos(angle), cy + reach * math.sin(angle)
        path = path.move_to(x, y) if corner == 0 else path.line_to(x, y)
    return path.close()


def scene(label: str) -> Canvas:
    """The whole scene: a filled star, a curved stroke, a ring with a hole, and a dashed rule."""
    face = Path.rounded_rectangle(Rect(0, 0, WIDTH, HEIGHT), 10)
    # A filled star: ten corners, alternating radii, one line_to each.
    badge = star(58, 72, 34)
    # One cubic through two control points, stroked with round caps and a round join. The caps
    # are why the ends are not cut off square.
    curve = Path().move_to(108, 96).cubic_to(150, 20, 220, 130, 262, 46)
    # A ring: two circles in one path, with the even-odd rule. The inner one is a hole rather
    # than a second disc, so the background shows through it - and the host hit tests the rule
    # it fills by, so the hole is not part of the region either.
    ring = circle(circle(Path(even_odd=True), 316, 72, 34), 316, 72, 16)
    # A dashed line: the dashes belong to the stroke, not to the path, so the path is two points.
    rule = Path().move_to(24, 158).line_to(WIDTH - 24, 158)
    return (
        Canvas()
        .fill(face, Brush.solid(0x181828FF))
        .fill(badge, Brush.solid(0xE0B050FF))
        .stroke_styled(
            curve,
            Brush.solid(0x8ECBFFFF),
            StrokeStyle(width=5, cap="round", join="round"),
        )
        .fill(ring, Brush.solid(0x70D090FF))
        .stroke_styled(
            rule,
            Brush.solid(0xFF8EA0FF),
            StrokeStyle(width=3, dashes=(10, 6)),
        )
        # An empty family asks the host for its default; custom font bytes are not supported.
        .text(label, Point(24, 180), 15, 0xC0C0D0FF)
        # Declaring a region lets the host report which part was pressed; the default hit area
        # is the whole window rectangle.
        .hit(CANVAS, face)
    )


def main() -> None:
    options = arguments(__doc__.splitlines()[0])
    deadline = None if options.duration is None else time.monotonic() + options.duration
    with OverlaySession.from_env() as session:
        with session.create_window(OverlayWindowOptions(Rect(40, 40, WIDTH, HEIGHT))) as window:
            window.present(scene("star, curve, ring, dashes - click or press Escape"))
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
                if isinstance(event, PointerEvent) and event.application_id == CANVAS and event.down:
                    break
                if isinstance(event, DismissedEvent):
                    break


if __name__ == "__main__":
    main()
