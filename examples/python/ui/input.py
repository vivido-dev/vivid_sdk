"""A text field: typing, selection, clipboard, and composition.

Python twin of `vivid_ui/examples/input`. Typing, selection, arrow keys, and copy/cut all
come from the protocol's own keys — the field answers HID usages, so the same code types the
same way on every host. Paste arrives as committed text through the host's own policy: a
terminal-hosted editor never reads the clipboard, it only writes one.

The caret is read off the measurement rather than counted in characters, and it is published
to the host each frame so an input method knows where to put its candidate window.
"""
from __future__ import annotations

from _ui import Keys, Mods, Frame, TextField, Ui, duration, platform
from vivid_sdk.overlay import ImeEvent, KeyEvent, Rect, TextEvent

PLACEHOLDER = "a line of text"


class Editor:
    def __init__(self) -> None:
        self.field = TextField("", size=16)
        self.copies = 0


def render(state: Editor, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x101018FF)
    f.label("Type here", Rect(20, 20, f.width - 40, 14), size=12, color=0x8080A0FF, vcenter=False)

    box = Rect(20, 46, f.width - 40, 40)
    focused = f.region(
        "field",
        box,
        radius=6,
        cursor="text",
        focusable=True,
        # A click focuses the field without claiming the keyboard put it there.
        on_click=lambda _event: f.ui.focus("field", visible=False),
    )
    f.box(box, bg=0x1C1C28FF, radius=6, border=1, border_color=0x30304AFF)
    if focused.focus_visible:
        f.focus_ring(box, radius=6)

    inner = Rect(box.x + 10, box.y, box.width - 20, box.height)
    field = state.field
    drawn = field.drawn()

    # The selection is drawn under the text, from the measured edges of what it covers.
    selection = field.selection_rect(f, inner)
    if selection is not None:
        f.box(selection, bg=0x2A4A7AFF)

    if drawn:
        f.label(drawn, inner, size=field.size, color=0xE6E6F0FF)
    else:
        f.label(PLACEHOLDER, inner, size=field.size, color=0x50506AFF)

    if focused.focused:
        caret = field.caret_rect(f, inner)
        f.box(caret, bg=0x8ECBFFFF)
        # Where the host should put an input method's candidate window.
        f.publish_caret(caret)

    f.label(
        f"{len(field.text)} characters, {state.copies} copied",
        Rect(20, box.y + 52, f.width - 40, 14),
        size=12,
        color=0x8080A0FF,
        vcenter=False,
    )


def make_ui() -> Ui:
    state = Editor()
    ui = Ui(state, render, width=420, height=240)
    command = platform()

    def key(event: KeyEvent) -> None:
        if event.down and not event.repeat and event.modifiers & command:
            # Copy and cut are the application's: the host writes the clipboard, and only
            # after a gesture in this window, which a key press is.
            if event.physical in (Keys.letter("c"), Keys.letter("x")):
                selected = state.field.selected_text()
                if selected:
                    ui.copy(selected)
                    state.copies += 1
                    if event.physical == Keys.letter("x"):
                        state.field.insert("")
                    ui.notify()
                return
        if event.down and not event.repeat and event.physical == Keys.TAB:
            ui.focus("field", visible=True)
            return
        # The field measures to move by clusters, and a key arrives between paints — so it
        # measures through the window, which is where the cache lives.
        if state.field.handle_key(event, ui):
            ui.notify()

    def text(event: TextEvent) -> None:
        if state.field.handle_text(event):
            ui.notify()

    def ime(event: ImeEvent) -> None:
        if state.field.handle_ime(event):
            ui.notify()

    ui.on_key = key
    ui.on_text = text
    ui.on_ime = ime
    return ui


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
