"""A focus ring that a keyboard shows and a click does not.

Python twin of `vivid_ui/examples/focus_visible`: focus that arrived from the keyboard is
drawn with a ring, focus that arrived from a click is not. That is the whole difference
between "the keyboard put it here" and "the pointer put it here", and it is why the helper
tracks the two separately.
"""
from __future__ import annotations

from typing import Any, Callable

from _ui import Keys, Mods, WHITE, Frame, Ui, duration
from vivid_sdk.overlay import KeyEvent, Point, Rect

BUTTONS = 3


class FocusRings:
    focused = 0


def render(state: FocusRings, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x101018FF)
    f.caption("Tab shows a ring; clicking does not", Point(20, 20))

    def clicked(index: int) -> Callable[[Any], None]:
        def handler(_event: KeyEvent) -> None:
            # A click focuses without claiming the keyboard put it there.
            state.focused = index
            f.ui.focus(f"button-{index}", visible=False)

        return handler

    row_y = 20 + 14 + 12
    for index in range(BUTTONS):
        f.button(
            f"button-{index}",
            Rect(20 + index * (96 + 10), row_y, 96, 36),
            str(index + 1),
            base=0x2A2A3CFF,
            hover=0x36364CFF,
            text_color=WHITE,
            focusable=True,
            on_click=clicked(index),
        )


def make_ui() -> Ui:
    state = FocusRings()
    ui = Ui(state, render, width=360, height=220)

    def handle_key(event: KeyEvent) -> None:
        if not (event.down and not event.repeat and event.physical == Keys.TAB):
            return
        step = -1 if event.modifiers & Mods.SHIFT else 1
        state.focused = (state.focused + step) % BUTTONS
        ui.focus(f"button-{state.focused}", visible=True)

    ui.on_key = handle_key
    return ui


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
