"""Linear fills at several angles and stop sets.

Python twin of `vivid_ui/examples/gradient`. The angle convention is CSS's — 0 is upward,
90 rightward — and the helper turns it into the two endpoints the wire wants, spanning the
box along the gradient's own direction. A hard stop is two stops at the same offset.
"""
from __future__ import annotations

from _ui import WHITE, Frame, Ui, duration, gradient_brush
from vivid_sdk.overlay import GradientStop, Path, Rect

PANELS = (
    ("to right", 90.0, ((0.0, 0xFF5060FF), (1.0, 0x5060FFFF))),
    ("to bottom", 180.0, ((0.0, 0x50D0A0FF), (1.0, 0x104060FF))),
    ("diagonal", 45.0, ((0.0, 0xFFE080FF), (0.5, 0xFF8060FF), (1.0, 0x603060FF))),
    # Two stops at the same offset is a hard edge rather than a blend.
    (
        "hard stop",
        90.0,
        ((0.0, 0x202030FF), (0.5, 0x202030FF), (0.5, 0x8080FFFF), (1.0, 0x8080FFFF)),
    ),
)


class Gradients:
    pass


def render(_state: Gradients, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x101018FF)
    for index, (label, angle, stops) in enumerate(PANELS):
        panel = Rect(20 + (index % 2) * (180 + 12), 20 + (index // 2) * (110 + 12), 180, 110)
        f.canvas.fill(
            Path.rounded_rectangle(panel, 8),
            gradient_brush(panel, angle, [GradientStop(offset, color) for offset, color in stops]),
        )
        f.label(
            label,
            Rect(panel.x + 8, panel.y + panel.height - 8 - 14, panel.width - 16, 14),
            size=12,
            color=WHITE,
        )


def make_ui() -> Ui:
    return Ui(Gradients(), render, width=420, height=300)


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
