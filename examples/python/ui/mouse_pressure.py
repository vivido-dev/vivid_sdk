"""Marks as wide as the device was pressed.

Python twin of `vivid_ui/examples/mouse_pressure`. Pressure is only sent by hardware that
reports it: a host with no such device sends nothing at all rather than zero, so a mark made
by an ordinary mouse falls back to the base width instead of collapsing to nothing.

That distinction is the example. It is also why pressure arrives as its own event beside the
move rather than as a field nobody can trust.
"""
from __future__ import annotations

from typing import Any, List, Tuple

from _ui import Frame, Ui, duration
from vivid_sdk.overlay import Point, PointerEvent, Rect

BASE_WIDTH = 6.0


def width_for(pressure: float) -> float:
    """How wide a mark is at this pressure. A host that reports none gives the base width."""
    return BASE_WIDTH * (0.4 + min(max(pressure, 0.0), 1.0) * 1.6)


class Pressure:
    def __init__(self) -> None:
        #: Each mark is a position and the pressure the device reported with it.
        self.points: List[Tuple[Point, float]] = []
        self.saw_pressure = False

    def push(self, position: Point, pressure: float) -> None:
        self.points.append((position, pressure))
        if pressure > 0:
            self.saw_pressure = True


def render(state: Pressure, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x101018FF)
    f.label(
        "pressure device detected" if state.saw_pressure else "press and draw",
        Rect(20, 20, f.width - 40, 14),
        size=12,
        color=0x8080A0FF,
        vcenter=False,
    )

    pad = Rect(20, 44, 380, 200)

    def pressed(event: PointerEvent) -> None:
        # A plain click has no pressure to report, so it draws at half — the value the
        # toolkit uses when a press arrives without a sensor behind it.
        state.push(event.position, 0.5)
        f.notify()

    def pressure(event: PointerEvent) -> None:
        if event.pressure is not None:
            state.push(event.position, event.pressure)
            f.notify()

    f.region("pad", pad, radius=8, cursor="crosshair", on_mouse_down=pressed, on_pressure=pressure)
    f.box(pad, bg=0x14141CFF, radius=8)

    # Each mark is a dot as wide as the device was pressed, drawn over the pad it is on.
    f.canvas.save()
    f.clip(pad, radius=8)
    for position, value in state.points:
        width = width_for(value)
        f.box(
            Rect(position.x - width / 2, position.y - width / 2, width, width),
            bg=0x8ECBFFFF,
            radius=width / 2,
        )
    f.canvas.restore()


def make_ui() -> Ui:
    return Ui(Pressure(), render, width=420, height=280)


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
