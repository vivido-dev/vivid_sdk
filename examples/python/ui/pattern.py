"""A tiled background, next to a gradient.

Python twin of `vivid_ui/examples/pattern`. There is no hatch primitive in the protocol, so
the tile is an image uploaded once and repeated — the same appearance, through the path a
sprite or a texture atlas would use. The repeat is the brush's, not a loop: one fill command
covers the whole box however large it is.
"""
from __future__ import annotations

from _ui import WHITE, Frame, Ui, duration, gradient_brush
from vivid_sdk.overlay import Brush, GradientStop, Path, Rect

SIZE = 8


def tile_pixels() -> bytes:
    """An 8 by 8 diagonal hatch, as RGBA8."""
    pixels = bytearray()
    for y in range(SIZE):
        for x in range(SIZE):
            # Every other diagonal is lighter, which reads as a slash pattern.
            lit = (x + y) % 4 < 2
            pixels.extend((0x50, 0x60, 0xE0, 0xFF) if lit else (0x1C, 0x1C, 0x2C, 0xFF))
    return bytes(pixels)


class Patterned:
    pass


def render(_state: Patterned, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x101018FF)

    hatched = Rect(20, 20, 360, 120)
    tile = f.upload("hatch", SIZE, SIZE, tile_pixels())
    f.canvas.fill(
        Path.rounded_rectangle(hatched, 6),
        # "repeat" is what makes one 8x8 upload cover 360 by 120.
        Brush.image(tile, None, "repeat"),
    )

    graded = Rect(20, 20 + 120 + 12, 360, 120)
    f.canvas.fill(
        Path.rounded_rectangle(graded, 6),
        gradient_brush(
            graded,
            135.0,
            [GradientStop(0.0, 0x203050FF), GradientStop(1.0, 0x50A0D0FF)],
        ),
    )
    f.label("gradient", Rect(graded.x + 10, graded.y + 8, 200, 16), size=13, color=WHITE)


def make_ui() -> Ui:
    return Ui(Patterned(), render, width=400, height=300)


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
