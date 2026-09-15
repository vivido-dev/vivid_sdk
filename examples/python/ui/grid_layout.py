"""A page laid out on grid tracks.

Python twin of `vivid_ui/examples/grid_layout`. Two fixed columns and three rows is the
kind of layout a grid engine is for, and without one it is the arithmetic a grid engine
does: a fixed 140px sidebar, the rest to the main column, the header and footer at their
content heights, and the body taking what is left. The window's own width decides which
arrangement applies — a geometry event repaints, so a resize re-runs the branch.
"""
from __future__ import annotations

from _ui import Frame, Ui, duration
from vivid_sdk.overlay import Rect

#: Past this, the sidebar sits beside the content; below it, above the content.
WIDE = 600.0

SIDEBAR = 140.0
GAP = 10.0
PAD = 16.0
HEADER = 44.0
FOOTER = 44.0


class Page:
    pass


def cell(f: Frame, box: Rect, label: str, color: int) -> None:
    f.box(box, bg=color, radius=6)
    f.label(label, box, size=13, color=0xF0F0F8FF, align="center")


def render(_state: Page, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x101018FF)
    wide = f.width >= WIDE

    left = PAD
    top = PAD
    width = f.width - 2 * PAD
    bottom = f.height - PAD

    cell(f, Rect(left, top, width, HEADER), "header", 0x30364CFF)
    body_top = top + HEADER + GAP
    body_bottom = bottom - FOOTER - GAP

    if wide:
        # Side by side: the sidebar takes its fixed track, the main column the rest.
        cell(f, Rect(left, body_top, SIDEBAR, body_bottom - body_top), "sidebar", 0x2A4A3AFF)
        cell(
            f,
            Rect(left + SIDEBAR + GAP, body_top, width - SIDEBAR - GAP, body_bottom - body_top),
            "main",
            0x1E2A44FF,
        )
    else:
        # Narrow: the sidebar collapses into a strip above the content.
        strip = 48.0
        cell(f, Rect(left, body_top, width, strip), "side", 0x2A4A3AFF)
        cell(
            f,
            Rect(left, body_top + strip + GAP, width, body_bottom - body_top - strip - GAP),
            "main",
            0x1E2A44FF,
        )

    cell(f, Rect(left, body_bottom + GAP, width, FOOTER), "footer", 0x3A2A3AFF)


def make_ui() -> Ui:
    return Ui(Page(), render, width=640, height=360)


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
