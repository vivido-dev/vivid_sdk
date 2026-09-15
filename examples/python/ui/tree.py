"""A box inside a box, 64 levels deep.

Python twin of `vivid_ui/examples/tree`. The Rust original is a lesson about layout cost —
an auto-sized box asks the engine for its content size at every level, and that question
compounds — which a port sidesteps entirely: each level's box is two pixels inside its
parent's, which is arithmetic, not a solver. What survives is the display-list half of the
lesson: 64 levels is 64 fills, 64 strokes, and 64 runs of text, and that is what a frame
this deep costs.
"""
from __future__ import annotations

from _ui import Frame, Ui, duration
from vivid_sdk.overlay import Rect

PAD = 2.0
LABEL_HEIGHT = 14.0


class DeepTree:
    def __init__(self, depth: int = 64) -> None:
        self.depth = depth


def render(state: DeepTree, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x0A0A12FF)
    box = Rect(8, 8, f.width - 16, f.height - 16)

    for depth in range(state.depth, -1, -1):
        if box.width <= 0 or box.height <= 0:
            break
        # The shade cycles every six levels, which is what makes the nesting readable.
        shade = 0x20 + (depth % 6) * 0x04
        f.box(box, bg=((0x101018 + shade * 0x0101) << 8) | 0xFF, border=1, border_color=0x00000040)
        f.label(
            str(depth),
            Rect(box.x + PAD, box.y + PAD, box.width - 2 * PAD, LABEL_HEIGHT),
            size=11,
            color=0xC0C0D0FF,
            vcenter=False,
        )
        # Each level sits inside its parent's padding, below the label it drew.
        box = Rect(
            box.x + PAD,
            box.y + PAD + LABEL_HEIGHT,
            max(box.width - 2 * PAD, 0.0),
            max(box.height - 2 * PAD - LABEL_HEIGHT, 0.0),
        )


def make_ui() -> Ui:
    return Ui(DeepTree(), render, width=500, height=500)


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
