"""Content taller than its box, scrolled by the wheel.

Python twin of `vivid_ui/examples/scrollable`: a fixed box clips a column of rows, and the
wheel moves the content under the clip. The rows past the edge are simply not drawn — the
same arithmetic a virtualized list is, written out once where it can be watched.
"""
from __future__ import annotations

from typing import Any

from _ui import Frame, Ui, duration
from vivid_sdk.overlay import Point, Rect, WheelEvent

ROW_HEIGHT = 28.0
VISIBLE_ROWS = 6
ROWS = 40


class Scrollable:
    def __init__(self) -> None:
        self.offset = 0.0

    def maximum_offset(self) -> float:
        """The largest offset that still leaves content on screen."""
        return max(ROWS * ROW_HEIGHT - VISIBLE_ROWS * ROW_HEIGHT, 0.0)

    def scroll_by(self, dy: float) -> None:
        """Move the content by a wheel delta. A positive `dy` is a scroll down, so the
        content moves up: the offset from the top of the content grows."""
        self.offset = min(max(self.offset + dy, 0.0), self.maximum_offset())


def render(state: Scrollable, f: Frame) -> None:
    viewport = Rect(0, 0, 300, VISIBLE_ROWS * ROW_HEIGHT)
    f.box(viewport, bg=0x101018FF)

    # The content is clipped: a row scrolled past the edge is neither drawn nor reachable,
    # and the clip ends where the rows end so the hit region is not clipped by it.
    f.canvas.save()
    f.clip(viewport)
    for index in range(ROWS):
        top = index * ROW_HEIGHT - state.offset
        if top > viewport.height or top + ROW_HEIGHT < 0:
            continue
        row = Rect(0, top, 300, ROW_HEIGHT)
        f.box(row, bg=0x1A1A24FF if index % 2 == 0 else 0x14141CFF)
        f.label(f"Row {index}", row, size=14, color=0xD0D0E0FF)
    f.canvas.restore()

    f.region("viewport", viewport, on_wheel=lambda event: wheeled(state, event, f))


def wheeled(state: Scrollable, event: Any, f: Frame) -> None:
    if isinstance(event, WheelEvent):
        state.scroll_by(event.dy)
        f.notify()


def make_ui() -> Ui:
    return Ui(Scrollable(), render, width=300, height=int(VISIBLE_ROWS * ROW_HEIGHT))


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
