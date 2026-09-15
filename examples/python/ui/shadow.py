"""The shadow token scale, and the parts of a shadow.

Python twin of `vivid_ui/examples/shadow`. A shadow is its own display-list command — a
rectangle, four radii, a colour, an offset, a blur, a spread, and an inset flag — so the
cards here are white boxes with one command behind each. This is also the one example on a
light background, which is what makes a soft shadow visible at all.
"""
from __future__ import annotations

from typing import Tuple

from _ui import Frame, Ui, duration, shadow_spec
from vivid_sdk.overlay import Point, Rect, Shadow

TOKENS = ("xs", "sm", "base", "md", "lg", "xl")

CARD = (120.0, 70.0)
GAP = 16.0
RADIUS = 8.0

#: The three past the scale: a hard offset, a spread, and one cast inward.
#: `(label, offset, blur, spread, colour, inset)`.
CUSTOM: Tuple[Tuple[str, Tuple[float, float], float, float, int, bool], ...] = (
    ("offset", (6.0, 6.0), 0.0, 0.0, 0x203050FF, False),
    ("spread", (0.0, 2.0), 6.0, 4.0, 0x80202080, False),
    ("inset", (0.0, 3.0), 8.0, 0.0, 0x00000060, True),
)


class Shadows:
    pass


def card(f: Frame, box: Rect, label: str, shadow: Shadow) -> None:
    """A white card with one shadow behind it. The shadow is cast first: it is what the
    card sits on, not something drawn over it."""
    f.canvas.shadow(shadow)
    f.box(box, bg=0xFFFFFFFF, radius=RADIUS)
    f.label(label, box, size=12, color=0x333344FF, align="center")


def render(_state: Shadows, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0xEDEDF2FF)

    for index, token in enumerate(TOKENS):
        box = Rect(24 + index * (CARD[0] + GAP), 24, CARD[0], CARD[1])
        (offset_x, offset_y), blur, spread = shadow_spec(token)
        card(
            f,
            box,
            token,
            Shadow(
                rect=box,
                radii=(RADIUS, RADIUS, RADIUS, RADIUS),
                color=0x0000000A,
                offset=Point(offset_x, offset_y),
                blur=blur,
                spread=spread,
                inset=False,
            ),
        )

    row_y = 24 + CARD[1] + GAP
    for index, (label, offset, blur, spread, color, inset) in enumerate(CUSTOM):
        box = Rect(24 + index * (CARD[0] + GAP), row_y, CARD[0], CARD[1])
        card(
            f,
            box,
            label,
            Shadow(
                rect=box,
                radii=(RADIUS, RADIUS, RADIUS, RADIUS),
                color=color,
                offset=Point(offset[0], offset[1]),
                blur=blur,
                spread=spread,
                inset=inset,
            ),
        )


def make_ui() -> Ui:
    return Ui(Shadows(), render, width=440, height=420)


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
