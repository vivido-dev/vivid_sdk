"""Ten thousand rows, of which a handful exist.

Python twin of `vivid_ui/examples/uniform_list`: a display list has a fixed command
ceiling, so a row nobody can see must not be built. Scrolling changes *which* rows exist,
not how many. The arithmetic is the helper's `ListState`, ported from the toolkit's own.
"""
from __future__ import annotations

from typing import Any

from _ui import Frame, ListState, Ui, duration
from vivid_sdk.overlay import Point, Rect, WheelEvent

ROWS = 10_000
ROW_HEIGHT = 24.0
#: p(16), a caption line, gap(8): what remains of a 320x260 window.
LIST_BOX = Rect(16, 38, 288, 204)


class Rows:
    def __init__(self) -> None:
        self.list = ListState(ROWS, ROW_HEIGHT)


def viewport_of(f: Frame) -> float:
    """The height the list was actually given: `bounds_of` is last frame's box, and on the
    first frame the declared height is the best estimate there is."""
    box = f.bounds_of("list")
    return box.height if box is not None else LIST_BOX.height


def render(state: Rows, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x101018FF)
    f.caption(f"{ROWS} rows, a dozen of them real", Point(16, 16))

    f.box(LIST_BOX, bg=0x14141CFF, radius=6)
    f.canvas.save()
    f.clip(LIST_BOX, radius=6)
    first, end = state.list.visible_range(viewport_of(f))
    for index in range(first, end):
        top = LIST_BOX.y + state.list.row_top(index)
        row = Rect(LIST_BOX.x, top, LIST_BOX.width, ROW_HEIGHT)
        f.box(row, bg=0x1A1A24FF if index % 2 == 0 else 0x14141CFF)
        # px(8): the label sits in the row, inset from its left edge.
        f.label(f"Row {index}", Rect(row.x + 8, row.y, row.width - 16, row.height), size=13, color=0xD0D0E0FF)
    f.canvas.restore()
    f.region("list", LIST_BOX, radius=6, on_wheel=lambda event: wheeled(state, event, f))


def wheeled(state: Rows, event: Any, f: Frame) -> None:
    if isinstance(event, WheelEvent):
        state.list.scroll_by(event.dy, viewport_of(f))
        f.notify()


def make_ui() -> Ui:
    return Ui(Rows(), render, width=320, height=260)


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
