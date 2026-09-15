"""A thousand star paths in one frame, against the frame's three ceilings.

Python twin of `vivid_ui/examples/paths_bench`. A display list has three ceilings and they
are not the same size: a path costs one *command* (4096 a frame), eleven *segments* (65536 a
frame), and about two hundred *bytes* once encoded — and it is the bytes that run out first,
because the scene a host grants is capped well below the profile's own ceiling. A window
this size is granted at most 256 KiB, which is where the default comes from: a thousand
stars fit, twelve hundred do not.
"""
from __future__ import annotations

import math
from typing import Any, Tuple

from _ui import Frame, Ui, duration
from vivid_sdk.overlay import Brush, Path, Rect

#: A star comes out as eleven segments: a move, nine lines, and a close.
SEGMENTS_PER_STAR = 11

#: The counts a click steps down through, so the frame can be seen shrinking.
STEPS = (1000, 500, 250, 125)

INK = 0xD0A860FF


class PathsBench:
    def __init__(self) -> None:
        self.step = 0

    @property
    def stars(self) -> int:
        return STEPS[self.step]

    def segments(self) -> int:
        return self.stars * SEGMENTS_PER_STAR

    def kilobytes(self) -> int:
        """Roughly what this frame's drawing weighs once encoded.

        The view cannot ask: the granted scene size is negotiated below the toolkit. A
        star's worth of coordinates encodes to about 220 bytes, which is close enough to
        show what the ceiling is doing.
        """
        return self.stars * 220 // 1024


def place(bounds: Rect, index: int, stars: int) -> Tuple[float, float, float]:
    """Where the `index`th star sits, in a grid that covers the box."""
    columns = max(int(math.ceil(math.sqrt(stars))), 1)
    rows = max((stars + columns - 1) // columns, 1)
    cell = (bounds.width / columns, bounds.height / rows)
    column, row = index % columns, index // columns
    return (
        bounds.x + cell[0] * (column + 0.5),
        bounds.y + cell[1] * (row + 0.5),
        min(cell[0], cell[1]) * 0.44,
    )


def star(cx: float, cy: float, radius: float) -> Path:
    """Ten corners alternating between two radii, starting upward."""
    path = Path()
    for corner in range(10):
        reach = radius if corner % 2 == 0 else radius * 0.45
        angle = -math.pi / 2 + corner * math.pi / 5
        x, y = cx + reach * math.cos(angle), cy + reach * math.sin(angle)
        path = path.move_to(x, y) if corner == 0 else path.line_to(x, y)
    return path.close()


def render(state: PathsBench, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x0C0C12FF)
    field = Rect(0, 0, f.width, f.height - 30)

    def halve(_event: Any) -> None:
        state.step = (state.step + 1) % len(STEPS)
        f.notify()

    f.region("bench", field, cursor="pointer", on_click=halve)
    stars = state.stars
    for index in range(stars):
        cx, cy, radius = place(field, index, stars)
        f.canvas.fill(star(cx, cy, radius), Brush.solid(INK))

    f.label(
        f"{stars} star paths · {state.segments()} of 65536 segments · "
        f"{stars + 1} of 4096 commands · {state.kilobytes()} of 256 KiB · click to halve",
        Rect(8, f.height - 26, f.width - 16, 18),
        size=11,
        color=0x8080A0FF,
    )


def make_ui() -> Ui:
    return Ui(PathsBench(), render, width=640, height=420)


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
