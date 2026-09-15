"""A counter, a button, and a line of text that follows it — the smallest app there is.

Python twin of `vivid_ui/examples/hello_world`: state renders itself into one frame, the
host does the hit testing, and nothing repaints until the state says it changed.
"""
from __future__ import annotations

from typing import Any

from _ui import BUTTON_ACTIVE, BUTTON_HOVER, BUTTON, TEXT, WHITE, Frame, Ui, duration
from vivid_sdk.overlay import Point, Rect


class Counter:
    count = 0

    def background(self) -> int:
        """The window's own background follows the count's parity, so a click is visible
        even before the label is read."""
        return 0x1E1E2EFF if self.count % 2 == 0 else 0x2A2A3CFF


def render(state: Counter, f: Frame) -> None:
    # p(20), gap(12): the label, then the button under it.
    f.box(Rect(0, 0, f.width, f.height), bg=state.background())
    f.label(
        f"Clicked {state.count} times",
        Rect(20, 20, f.width - 40, 28),
        size=24,
        color=TEXT,
        vcenter=False,
    )
    text = "Increment"
    width = f.measure(text, 13).width
    f.button(
        "increment",
        Rect(20, 60, width + 28, 32),
        text,
        base=BUTTON,
        hover=BUTTON_HOVER,
        active=BUTTON_ACTIVE,
        on_click=lambda _event: bump(state, f),
    )


def bump(state: Counter, f: Frame) -> None:
    state.count += 1
    f.notify()


def make_ui() -> Ui:
    return Ui(Counter(), render, width=420, height=240)


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
