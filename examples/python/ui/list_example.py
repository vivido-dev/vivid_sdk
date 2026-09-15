"""A log pinned to its newest line, with a scrollbar.

Python twin of `vivid_ui/examples/list_example`. Two things make a log a log rather than a
list: it follows its own tail until the reader scrolls away from it, and it takes that up
again when the reader comes back. Both are one comparison against the maximum offset.

The scrollbar is drawing, not a special case: a track, and a thumb whose length is the
fraction of the content on screen and whose position is how far through it that fraction is.
"""
from __future__ import annotations

from typing import Any

from _ui import Frame, Keys, ListState, Ui, duration
from vivid_sdk.overlay import KeyEvent, Rect, WheelEvent

ROW_HEIGHT = 22.0
VIEWPORT = 200.0


class Log:
    def __init__(self) -> None:
        self.entries = [
            f"[{index:04}] the log said something worth keeping" for index in range(200)
        ]
        self.list = ListState(len(self.entries), ROW_HEIGHT)
        self.list.scroll_to(self.list.maximum_offset(VIEWPORT), VIEWPORT)
        #: Stay at the bottom as entries arrive, until the reader scrolls away.
        self.follow = True

    def push(self, entry: str, viewport: float) -> None:
        """Append, following the tail if the reader has not scrolled away from it."""
        self.entries.append(entry)
        self.list.set_count(len(self.entries), viewport)
        if self.follow:
            self.list.scroll_to(self.list.maximum_offset(viewport), viewport)

    def at_tail(self, viewport: float) -> bool:
        """Whether the list is at its end, which is what "following" means."""
        return abs(self.list.offset - self.list.maximum_offset(viewport)) < 1.0


def render(state: Log, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x101018FF)
    f.label(
        "following the tail" if state.follow else "scrolled back",
        Rect(16, 16, f.width - 32, 14),
        size=12,
        color=0x8080A0FF,
        vcenter=False,
    )

    box = Rect(16, 38, f.width - 32 - 12, VIEWPORT)

    def scrolled(event: Any) -> None:
        if isinstance(event, WheelEvent):
            state.list.scroll_by(event.dy, box.height)
            # Letting go of the tail is a scroll away from it; coming back takes it up again.
            state.follow = state.at_tail(box.height)
            f.notify()

    f.region("log", box, radius=6, on_wheel=scrolled)
    f.box(box, bg=0x16161EFF, radius=6)

    # Only the rows the box can show exist. The one past the bottom is the one sliding in.
    f.canvas.save()
    f.clip(box, radius=6)
    first, last = state.list.visible_range(box.height)
    for index in range(first, last):
        top = box.y + state.list.row_top(index)
        f.label(
            state.entries[index],
            Rect(box.x + 8, top, box.width - 16, ROW_HEIGHT),
            size=12,
            color=0xC0C0D0FF,
        )
    f.canvas.restore()

    track = Rect(box.x + box.width + 4, box.y, 8, box.height)
    f.scrollbar(track, state.list, box.height)


def make_ui() -> Ui:
    state = Log()
    ui = Ui(state, render, width=420, height=260)

    def appended(event: KeyEvent) -> None:
        if not (event.down and not event.repeat and event.physical == Keys.ENTER):
            return
        viewport = ui.bounds_of("log")
        state.push(
            f"[{len(state.entries):04}] appended by hand",
            viewport.height if viewport is not None else VIEWPORT,
        )
        ui.notify()

    ui.on_key = appended
    return ui


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
