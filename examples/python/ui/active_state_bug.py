"""A button that must not stay pressed.

Python twin of `vivid_ui/examples/active_state_bug`, and the regression test for the whole
press contract: a press is active exactly while the button is held over the element it
started on, a release anywhere ends it, and leaving the element while pressed cancels it —
because a release over nothing is not reported at all.
"""
from __future__ import annotations

from _ui import TEXT, WHITE, Frame, Ui, duration
from vivid_sdk.overlay import Rect


class Toggle:
    presses = 0


def render(state: Toggle, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x14141CFF)
    f.label(
        f"Pressed {state.presses} times",
        Rect(20, 20, f.width - 40, 18),
        size=14,
        color=0xC0C0D0FF,
        vcenter=False,
    )
    f.button(
        "press",
        Rect(20, 48, 160, 44),
        "Hold me",
        base=0x3A3A52FF,
        hover=0x4A4A68FF,
        # Amber is the active colour, so a stuck press would be unmistakable.
        active=0x8A6A20FF,
        text_color=WHITE,
        on_click=lambda _event: count(state, f),
    )


def count(state: Toggle, f: Frame) -> None:
    state.presses += 1
    f.notify()


def make_ui() -> Ui:
    return Ui(Toggle(), render, width=320, height=160)


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
