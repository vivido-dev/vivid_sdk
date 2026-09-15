"""Tab and shift-tab moving focus between fields.

Python twin of `vivid_ui/examples/tab_stop`. Tab order is paint order, which is the only
order a display list has — a frame draws its regions in sequence, and that sequence is what
"next" means. Clicking a field focuses it too, but without the ring: only the keyboard
claims to have put the focus there.
"""
from __future__ import annotations

from typing import Any, Callable, List

from _ui import Keys, Mods, Frame, TextField, Ui, duration
from vivid_sdk.overlay import ImeEvent, KeyEvent, Rect, TextEvent

LABELS = ("first", "second", "third")


class TabStops:
    def __init__(self) -> None:
        self.fields: List[TextField] = [TextField("", size=14) for _ in LABELS]

    def under_focus(self, ui: Ui) -> int:
        """Which field the keys are going to, if any."""
        focused = ui.focused()
        for index in range(len(self.fields)):
            if focused == f"row-{index}":
                return index
        return -1


def render(state: TabStops, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x101018FF)

    def clicked(index: int) -> Callable[[Any], None]:
        def handler(_event: Any) -> None:
            # A click focuses without claiming the keyboard put it there.
            f.ui.focus(f"row-{index}", visible=False)

        return handler

    for index, field in enumerate(state.fields):
        row = Rect(20, 20 + index * (44 + 8), f.width - 40, 44)
        element = f.region(
            f"row-{index}",
            row,
            radius=6,
            cursor="text",
            focusable=True,
            on_click=clicked(index),
        )
        active = element.focused
        f.box(
            row,
            bg=0x24304AFF if active else 0x181824FF,
            radius=6,
            border=1,
            border_color=0x8ECBFFFF if active else 0x2A2A3CFF,
        )
        if element.focus_visible:
            f.focus_ring(row, radius=6)

        inner = Rect(row.x + 8, row.y, row.width - 16, row.height)
        selection = field.selection_rect(f, inner)
        if selection is not None:
            f.box(selection, bg=0x2A4A7AFF)
        drawn = field.drawn()
        if drawn:
            f.label(drawn, inner, size=field.size, color=0xE6E6F0FF)
        else:
            f.label(LABELS[index], inner, size=field.size, color=0x50506AFF)
        if active:
            f.box(field.caret_rect(f, inner), bg=0x8ECBFFFF)


def make_ui() -> Ui:
    state = TabStops()
    ui = Ui(state, render, width=360, height=240)

    def key(event: KeyEvent) -> None:
        if event.down and not event.repeat and event.physical == Keys.TAB:
            ui.focus_next(backwards=bool(event.modifiers & Mods.SHIFT), visible=True)
            return
        index = state.under_focus(ui)
        if index >= 0 and state.fields[index].handle_key(event, ui):
            ui.notify()

    def text(event: TextEvent) -> None:
        index = state.under_focus(ui)
        if index >= 0 and state.fields[index].handle_text(event):
            ui.notify()

    def ime(event: ImeEvent) -> None:
        index = state.under_focus(ui)
        if index >= 0 and state.fields[index].handle_ime(event):
            ui.notify()

    ui.on_key = key
    ui.on_text = text
    ui.on_ime = ime
    return ui


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
