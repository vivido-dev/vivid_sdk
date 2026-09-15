"""A title bar that moves the window, beside the edges that resize it.

Python twin of `vivid_ui/examples/window_movable`. Both are the same mechanism: a region's
*role* tells the host what a drag on it means — move the window, or resize it from that
edge — and the host does the work natively, without a frame in between.

An overlay window has no titlebar of its own; the terminal behind it does. So what is
movable here is the part of the pane the application nominates.
"""
from __future__ import annotations

from typing import Any

from _ui import Frame, Ui, duration
from vivid_sdk.overlay import Rect

# The edges are the same four this window's sibling declares, so they are shared rather
# than written twice — the Rust views duplicate the table between them.
from window_shadow import EDGES, edge_box

PAD = 12.0
TITLE_BAR = 30.0


class Movable:
    #: How many drags ended here — what a view does with the notification, the host having
    #: already done the moving.
    drags = 0


def render(state: Movable, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x0C0C14FF)
    panel = Rect(PAD, PAD, f.width - 2 * PAD, f.height - 2 * PAD)
    f.box(panel, bg=0x1A1A28FF, radius=8, border=1, border_color=0x3A3A52FF)

    def dragged(_event: Any) -> None:
        # A drag ends with a release the view can hear about, which is what "remember where
        # the window was put" would hang off.
        state.drags += 1
        f.notify()

    # The bar is a drag handle first and an element second. It carries no click handler for
    # the same reason: the host is already moving the window, and a region that both moved
    # it and reported a click would do both.
    bar = Rect(panel.x, panel.y, panel.width, TITLE_BAR)
    f.region("title-bar", bar, role="drag", cursor="pointer", on_mouse_up=dragged)
    f.box(bar, bg=0x26263CFF, radius=8)
    # Square off the bar's own bottom corners, so only the window's top ones are round.
    f.box(Rect(bar.x, bar.y + TITLE_BAR / 2, bar.width, TITLE_BAR / 2), bg=0x26263CFF)
    f.label("drag this bar to move the window", bar, size=12, color=0xC0C0D0FF, align="center")

    f.label(
        "the four edges resize, the bar moves",
        Rect(panel.x + 14, bar.y + TITLE_BAR + 14, panel.width - 28, 16),
        size=12,
        color=0x8080A0FF,
        vcenter=False,
    )
    f.label(
        f"{state.drags} drags so far",
        Rect(panel.x + 14, bar.y + TITLE_BAR + 14 + 16 + 6, panel.width - 28, 16),
        size=12,
        color=0x606078FF,
        vcenter=False,
    )

    for name, edges, cursor in EDGES:
        f.resize_region(f"edge-{name}", edge_box(name, panel), edges, cursor)


def make_ui() -> Ui:
    return Ui(Movable(), render, width=384, height=264)


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
