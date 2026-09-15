"""Tiles dragged onto a tray, with pointer capture.

Python twin of `vivid_ui/examples/drag_drop`. A drag is three events and one piece of
state: a press records what is moving, moves carry it, and the release decides where it
landed. Capture is what makes the middle one work — without it the moves stop the moment
the pointer leaves the tile it started on, and the release is never heard at all.

Where the tray *is* comes from the frame before this one: the host hit-tested against that
frame, so it is the only geometry an event can be talking about.
"""
from __future__ import annotations

from typing import Any, List, Optional, Tuple

from _ui import WHITE, Frame, Ui, duration
from vivid_sdk.overlay import Point, PointerEvent, Rect

TILES = ("alpha", "beta", "gamma")


class DragDrop:
    def __init__(self) -> None:
        #: The tile being dragged, and where the pointer has it.
        self.dragging: Optional[Tuple[int, Point]] = None
        self.dropped: List[int] = []

    def is_dropped(self, index: int) -> bool:
        return index in self.dropped


def render(state: DragDrop, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x101018FF)
    f.caption("Drag a tile into the tray", Point(20, 20))

    def grabbed(index: int) -> Any:
        def handler(event: PointerEvent) -> None:
            state.dragging = (index, event.position)
            # Capture keeps the moves coming after the pointer leaves the tile, so the
            # release is heard wherever it happens.
            f.ui.capture(True)
            f.notify()

        return handler

    row_y = 46.0
    for index, label in enumerate(TILES):
        tile = Rect(20 + index * (64 + 10), row_y, 64, 40)
        held = state.dragging is not None and state.dragging[0] == index
        f.region(f"tile-{index}", tile, radius=6, cursor="grab", on_mouse_down=grabbed(index))
        f.box(tile, bg=0x8A6A20FF if held else 0x3A4A6AFF, radius=6)
        f.label(label, tile, size=12, color=WHITE, align="center")

    tray = Rect(20, row_y + 40 + 12, f.width - 40, 64)
    f.region("tray", tray, radius=8)
    f.box(tray, radius=8, border=1, border_color=0x50506AFF)
    for slot, index in enumerate(state.dropped):
        chip = Rect(tray.x + 10 + slot * (48 + 8), tray.y + (tray.height - 28) / 2, 48, 28)
        f.box(chip, bg=0x2A4A3AFF, radius=4)
        f.label(TILES[index], chip, size=11, color=0xC0E0D0FF, align="center")

    # The tile being dragged follows the pointer, drawn last so it is over everything.
    if state.dragging is not None:
        index, position = state.dragging
        ghost = Rect(position.x, position.y, 64, 40)
        f.box(ghost, bg=0x6A7AB0FF, radius=6)
        f.label(TILES[index], ghost, size=12, color=WHITE, align="center")


def make_ui() -> Ui:
    state = DragDrop()
    ui = Ui(state, render, width=420, height=260)

    def moved(event: PointerEvent) -> None:
        if state.dragging is not None:
            state.dragging = (state.dragging[0], event.position)
            ui.notify()

    def released(event: PointerEvent) -> None:
        if state.dragging is None:
            return
        index, position = state.dragging
        state.dragging = None
        # Dropping on the tray is dropping inside it; anywhere else puts the tile back.
        tray = ui.bounds_of("tray")
        inside = tray is not None and (
            tray.x <= position.x < tray.x + tray.width
            and tray.y <= position.y < tray.y + tray.height
        )
        if inside and not state.is_dropped(index):
            state.dropped.append(index)
        ui.capture(False)
        ui.notify()

    ui.on_mouse_move = moved
    # A release outside every region is reported to nobody, which is what capture is for:
    # with it the release arrives here, naming whatever region it happened over.
    ui.on_mouse_up = released
    return ui


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
