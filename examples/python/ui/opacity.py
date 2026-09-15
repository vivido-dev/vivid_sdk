"""A subtree painted as one translucent group.

Python twin of `vivid_ui/examples/opacity`: the group's opacity follows where the pointer
is — left is transparent, right is solid — and everything inside the group fades together,
because the display list carries it as one layer rather than as parts.
"""
from __future__ import annotations

from typing import Any, Callable

from _ui import Frame, Ui, duration
from vivid_sdk.overlay import Point, PointerEvent, Rect

WIDTH = 360.0
HEIGHT = 260.0


class Fade:
    opacity = 1.0

    @staticmethod
    def from_pointer(x: float, width: float) -> float:
        """Opacity from a pointer position across the window: left is transparent, right
        is solid."""
        return min(max(x / width, 0.0), 1.0)


def render(state: Fade, f: Frame) -> None:
    f.region(
        "surface",
        Rect(0, 0, f.width, f.height),
        on_mouse_move=moved(state, f),
    )
    f.box(Rect(0, 0, f.width, f.height), bg=0x141420FF)
    f.label(
        "Move the pointer left and right",
        Rect(20, 20, f.width - 40, 16),
        size=13,
        color=0xA0A0B8FF,
        vcenter=False,
    )

    # One translucent group: everything between the save and the restore is a layer the
    # host composites at this opacity, so the bar and both runs of text fade together.
    f.canvas.save()
    f.canvas.opacity(state.opacity)
    f.box(Rect(20, 46, f.width - 40, 100), bg=0x4A5FD0FF, radius=8)
    f.label("Fading group", Rect(32, 58, 200, 24), size=20, color=0xFFFFFFFF, vcenter=False)
    f.label(
        "Everything in here is one group",
        Rect(32, 88, 260, 16),
        size=13,
        color=0xD0D0E0FF,
        vcenter=False,
    )
    f.box(Rect(32, 110, 120, 24), bg=0x9ECBFFFF, radius=4)
    f.canvas.restore()


def moved(state: Fade, f: Frame) -> Callable[[Any], None]:
    def handler(event: Any) -> None:
        if isinstance(event, PointerEvent):
            next_opacity = Fade.from_pointer(event.position.x, WIDTH)
            if abs(next_opacity - state.opacity) > 0.001:
                state.opacity = next_opacity
                f.notify()

    return handler


def make_ui() -> Ui:
    return Ui(Fade(), render, width=WIDTH, height=HEIGHT)


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
