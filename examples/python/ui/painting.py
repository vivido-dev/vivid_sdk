"""Freeform paths: polygons, curves, an even-odd hole, dashes, and freehand drawing.

Python twin of `vivid_ui/examples/painting`. Everything else the toolkit paints is a box, a
run of text, or a picture; this is the escape hatch, and in a port there is no hatch to
reach for — the display list *is* the API, so these figures are written the same way the
toolkit writes them internally.

The ring is the one to watch: two subpaths filled by the even-odd rule, which makes the
inner circle a hole rather than a second disc. The host hit tests the rule it fills by, so
that hole is not clickable either.
"""
from __future__ import annotations

import math
from typing import Any, List

from _ui import Frame, Ui, duration
from vivid_sdk.overlay import Brush, Path, Point, PointerEvent, Rect, StrokeStyle

#: The circle constant, for the cubic approximation of a round shape.
KAPPA = 0.552_284_749_830_793_6

FIGURE = 104.0


def circle(path: Path, cx: float, cy: float, radius: float) -> Path:
    """A circle as four cubics, appended to a path that may already have subpaths."""
    k = radius * KAPPA
    return (
        path.move_to(cx, cy - radius)
        .cubic_to(cx + k, cy - radius, cx + radius, cy - k, cx + radius, cy)
        .cubic_to(cx + radius, cy + k, cx + k, cy + radius, cx, cy + radius)
        .cubic_to(cx - k, cy + radius, cx - radius, cy + k, cx - radius, cy)
        .cubic_to(cx - radius, cy - k, cx - k, cy - radius, cx, cy - radius)
        .close()
    )


def star(box: Rect) -> Path:
    """Ten corners alternating between two radii, starting upward."""
    path = Path()
    cx, cy = box.x + box.width / 2, box.y + box.height / 2
    for corner in range(10):
        reach = box.width * (0.44 if corner % 2 == 0 else 0.18)
        angle = -math.pi / 2 + corner * math.pi / 5
        x, y = cx + reach * math.cos(angle), cy + reach * math.sin(angle)
        path = path.move_to(x, y) if corner == 0 else path.line_to(x, y)
    return path.close()


def round_stroke(width: float) -> StrokeStyle:
    """A round-capped stroke, which is what a freehand line wants at both ends."""
    return StrokeStyle(width=width, cap="round", join="round")


class Painting:
    def __init__(self) -> None:
        #: One stroke per press: the points the pointer visited while it was held down.
        self.strokes: List[List[Point]] = []
        self.drawing = False

    def stroke_count(self) -> int:
        return len(self.strokes)


def render(state: Painting, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x101018FF)
    f.label(
        f"filled, stroked, even-odd, dashed — {state.stroke_count()} strokes on the pad",
        Rect(16, 16, f.width - 32, 14),
        size=11,
        color=0x8080A0FF,
        vcenter=False,
    )

    row_y = 38.0
    wells = [Rect(16 + index * (FIGURE + 10), row_y, FIGURE, FIGURE) for index in range(4)]
    for well in wells:
        f.box(well, bg=0x1A1A24FF, radius=6)

    # A filled polygon.
    f.canvas.fill(star(wells[0]), Brush.solid(0xE0B050FF))

    # One cubic, stroked with round caps and a round join.
    box = wells[1]
    f.canvas.stroke_styled(
        Path()
        .move_to(box.x + box.width * 0.12, box.y + box.height * 0.82)
        .cubic_to(
            box.x + box.width * 0.3,
            box.y + box.height * 0.02,
            box.x + box.width * 0.7,
            box.y + box.height * 0.98,
            box.x + box.width * 0.88,
            box.y + box.height * 0.18,
        ),
        Brush.solid(0x8ECBFFFF),
        round_stroke(5),
    )

    # Two nested circles, filled by the even-odd rule: a shape with a hole in it.
    box = wells[2]
    cx, cy = box.x + box.width / 2, box.y + box.height / 2
    ring = circle(Path(even_odd=True), cx, cy, box.width * 0.44)
    f.canvas.fill(circle(ring, cx, cy, box.width * 0.2), Brush.solid(0x70D090FF))

    # A dashed stroke: the dashes belong to the style, so the path is one straight line.
    box = wells[3]
    f.canvas.stroke_styled(
        Path()
        .move_to(box.x + box.width * 0.1, box.y + box.height * 0.5)
        .line_to(box.x + box.width * 0.9, box.y + box.height * 0.5),
        Brush.solid(0xFF8EA0FF),
        StrokeStyle(width=4, cap="round", join="round", dashes=(9, 6)),
    )

    # The pad: each stroke is a path through exactly the points the host reported, in the
    # same window coordinates the canvas paints in — nothing in between to get wrong.
    pad = Rect(16, row_y + FIGURE + 10, f.width - 32, f.height - row_y - FIGURE - 26)

    def began(event: PointerEvent) -> None:
        state.strokes.append([event.position])
        state.drawing = True
        f.notify()

    def moved(event: PointerEvent) -> None:
        if state.drawing and state.strokes:
            state.strokes[-1].append(event.position)
            f.notify()

    def ended(_event: Any) -> None:
        state.drawing = False
        f.notify()

    f.region(
        "freehand",
        pad,
        radius=8,
        cursor="crosshair",
        on_mouse_down=began,
        on_mouse_move=moved,
        on_mouse_up=ended,
    )
    f.box(pad, bg=0x14141CFF, radius=8)
    f.canvas.save()
    f.clip(pad, radius=8)
    for points in state.strokes:
        if len(points) < 2:
            # A press that has not moved is a dot nobody asked for: the pad reports where
            # the pointer went, and it has not gone anywhere yet.
            continue
        path = Path().move_to(points[0].x, points[0].y)
        for point in points[1:]:
            path = path.line_to(point.x, point.y)
        f.canvas.stroke_styled(path, Brush.solid(0xFFD070FF), round_stroke(3))
    f.canvas.restore()


def make_ui() -> Ui:
    return Ui(Painting(), render, width=472, height=300)


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
