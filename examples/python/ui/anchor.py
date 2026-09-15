"""Panels placed against each corner of their parent.

Python twin of `vivid_ui/examples/anchor`. The toolkit spells these as an anchor plus an
offset and lets layout resolve them; a port has the parent's box in hand, so each corner is
one line of arithmetic. Painting them last is what puts them over the field they sit on —
in a display list, paint order is the whole of z-order.
"""
from __future__ import annotations

from _ui import WHITE, Frame, Ui, duration
from vivid_sdk.overlay import Point, Rect

#: Each corner, as a fraction of the field plus the direction the 8px offset pushes.
CORNERS = (
    ("top left", 0.0, 0.0),
    ("top center", 0.5, 0.0),
    ("top right", 1.0, 0.0),
    ("bottom left", 0.0, 1.0),
    ("bottom center", 0.5, 1.0),
    ("bottom right", 1.0, 1.0),
)

OFFSET = 8.0
PAD = 6.0


class Anchors:
    pass


def render(_state: Anchors, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x101018FF)
    field = Rect(16, 16, 380, 220)
    f.box(field, bg=0x1C1C2CFF, radius=8, border=1, border_color=0x30304AFF)
    f.caption("the anchor's parent", Point(field.x + 8, field.y + 8))

    for label, fx, fy in CORNERS:
        measured = f.measure(label, 11)
        width = measured.width + PAD * 2
        height = measured.height + PAD * 2
        # The offset pushes inward from whichever edge the anchor names, and a centered
        # panel is centered rather than offset.
        x = field.x + (field.width - width) * fx + (OFFSET if fx == 0 else -OFFSET if fx == 1 else 0)
        y = field.y + (field.height - height) * fy + (OFFSET if fy == 0 else -OFFSET)
        panel = Rect(x, y, width, height)
        f.box(panel, bg=0x4A5FD0FF, radius=4)
        f.label(label, panel, size=11, color=WHITE, align="center")


def make_ui() -> Ui:
    return Ui(Anchors(), render, width=420, height=260)


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
