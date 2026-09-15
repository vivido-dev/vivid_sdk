"""A frame whose edges ask the host to resize.

Python twin of `vivid_ui/examples/window_shadow`. The edges are hit regions whose role is
"resize", so the *host* performs the resize: the window grows or shrinks natively as an edge
is dragged, and the frame redraws at whatever size it ends up. Nothing here reimplements
window resizing — naming the region is the whole of it.
"""
from __future__ import annotations

from typing import Tuple

from _ui import (
    EDGE_BOTTOM,
    EDGE_LEFT,
    EDGE_RIGHT,
    EDGE_TOP,
    Frame,
    Ui,
    duration,
)
from vivid_sdk.overlay import CursorShape, Rect

PAD = 12.0
THICK = 6.0

#: Each edge: the bitmask the host reads, and the shape the pointer takes there.
EDGES: Tuple[Tuple[str, int, CursorShape], ...] = (
    ("left", EDGE_LEFT, "resize-left"),
    ("right", EDGE_RIGHT, "resize-right"),
    ("top", EDGE_TOP, "resize-up"),
    ("bottom", EDGE_BOTTOM, "resize-down"),
)


class WindowFrame:
    pass


def edge_box(name: str, panel: Rect) -> Rect:
    """Where one edge's strip sits: a thin band just inside the frame it resizes."""
    if name == "left":
        return Rect(panel.x, panel.y, THICK, panel.height)
    if name == "right":
        return Rect(panel.x + panel.width - THICK, panel.y, THICK, panel.height)
    if name == "top":
        return Rect(panel.x, panel.y, panel.width, THICK)
    return Rect(panel.x, panel.y + panel.height - THICK, panel.width, THICK)


def render(_state: WindowFrame, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x0C0C14FF)
    panel = Rect(PAD, PAD, f.width - 2 * PAD, f.height - 2 * PAD)
    f.box(panel, bg=0x1A1A28FF, radius=8, border=1, border_color=0x3A3A52FF)
    f.label(
        "Drag an edge to resize",
        Rect(panel.x + 14, panel.y + 14, panel.width - 28, 18),
        size=13,
        color=0xC0C0D0FF,
        vcenter=False,
    )
    # Painted last, so they sit over the frame they belong to.
    for name, edges, cursor in EDGES:
        f.resize_region(f"edge-{name}", edge_box(name, panel), edges, cursor)


def make_ui() -> Ui:
    return Ui(WindowFrame(), render, width=360, height=260)


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
