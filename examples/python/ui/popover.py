"""A floating panel anchored to the button that opened it.

Python twin of `vivid_ui/examples/popover`: the panel paints over everything it overlaps
(late in the display list is on top), and a scrim behind it takes the clicks meant for the
rest of the window — which is how clicking outside closes it. The scrim is an ordinary
region rather than a blocking one: it has to *hear* the click that closes the panel, and a
blocking region reports nothing by design.
"""
from __future__ import annotations

from typing import Any, Callable

from _ui import Frame, Ui, duration
from vivid_sdk.overlay import Point, Rect

WIDTH = 420.0
HEIGHT = 300.0


class Popover:
    open = False


def render(state: Popover, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x101018FF)
    f.caption("Anchored to the button above", Point(20, 20))
    f.button(
        "menu-button",
        Rect(20, 44, 140, 36),
        "Open menu",
        on_click=toggle(state, f),
    )

    if not state.open:
        return

    # The scrim covers the window, painted over the content it silences. Clicking it is
    # what closes the panel — so it is a region that hears clicks, not one that blocks.
    f.region("scrim", Rect(0, 0, f.width, f.height), on_click=close(state, f))
    f.box(Rect(0, 0, f.width, f.height), bg=0x00000066)

    # The menu paints after the scrim, so it sits over it: paint order is z-order.
    menu = Rect(0, 46, 200, 72)
    f.shadow_box(menu, "lg", radius=8)
    f.box(menu, bg=0x24243AFF, radius=8)
    f.region("menu-copy", Rect(6, 52, 120, 20), on_click=close(state, f))
    f.label("Copy", Rect(6, 52, 120, 20), size=14, color=0xE6E6F0FF)
    f.label("Paste", Rect(6, 72, 120, 20), size=14, color=0xE6E6F0FF)
    f.label("Select all", Rect(6, 92, 120, 20), size=14, color=0xE6E6F0FF)


def toggle(state: Popover, f: Frame) -> Callable[[Any], None]:
    def handler(_event: Any) -> None:
        state.open = not state.open
        f.notify()

    return handler


def close(state: Popover, f: Frame) -> Callable[[Any], None]:
    def handler(_event: Any) -> None:
        state.open = False
        f.notify()

    return handler


def make_ui() -> Ui:
    return Ui(Popover(), render, width=WIDTH, height=HEIGHT)


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
