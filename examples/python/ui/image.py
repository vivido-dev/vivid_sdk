"""One picture, fitted into its box four ways.

Python twin of `vivid_ui/examples/image`. The Rust original encodes a PNG and decodes it
again to prove the round trip; the protocol carries raw pixels either way, and neither
stdlib Python nor Node has a decoder, so this port generates the checkerboard directly as
RGBA and uploads that. The lesson is unchanged: a picture reaches the host as an upload and
a rectangle, and the fit is arithmetic the producer does.
"""
from __future__ import annotations

from _ui import CAPTION, Frame, Ui, duration, fit_rect
from vivid_sdk.overlay import Rect

WIDTH, HEIGHT, SQUARE = 64, 32, 8
SLOT = (96.0, 72.0)


def checkerboard() -> bytes:
    """A checkerboard, because how a picture is fitted into a box is obvious in one and
    invisible in a photograph."""
    pixels = bytearray()
    for y in range(HEIGHT):
        for x in range(WIDTH):
            dark = ((x // SQUARE) + (y // SQUARE)) % 2 == 0
            pixels.extend((0x20, 0x20, 0x30, 0xFF) if dark else (0x70, 0x80, 0xD0, 0xFF))
    return bytes(pixels)


class Images:
    pass


def render(_state: Images, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x101018FF)
    picture = f.upload("checkerboard", WIDTH, HEIGHT, checkerboard())

    def slot(index: int, row: int, label: str) -> Rect:
        """A labelled well, so the only difference between the variants is the fit."""
        x = 20 + index * (SLOT[0] + 12)
        y = 20 + row * (SLOT[1] + 12 + 16 + 4)
        f.label(label, Rect(x, y, SLOT[0], 13), size=11, color=CAPTION, vcenter=False)
        box = Rect(x, y + 17, SLOT[0], SLOT[1])
        f.box(box, bg=0x1A1A24FF, radius=4)
        return box

    # Natural size: the picture's own pixels, centred in the well rather than scaled.
    box = slot(0, 0, "natural size")
    f.image(
        picture,
        Rect(
            box.x + (box.width - WIDTH) / 2,
            box.y + (box.height - HEIGHT) / 2,
            float(WIDTH),
            float(HEIGHT),
        ),
    )

    # Contain: the whole picture, as large as fits, proportions kept.
    f.image(picture, fit_rect(WIDTH, HEIGHT, slot(1, 0, "contain")))

    # Cover: the box filled, proportions kept, the overflow clipped away.
    box = slot(2, 0, "cover")
    f.canvas.save()
    f.clip(box, radius=4)
    scale = max(box.width / WIDTH, box.height / HEIGHT)
    f.image(
        picture,
        Rect(
            box.x + (box.width - WIDTH * scale) / 2,
            box.y + (box.height - HEIGHT * scale) / 2,
            WIDTH * scale,
            HEIGHT * scale,
        ),
    )
    f.canvas.restore()

    # Fill: the box exactly, proportions abandoned.
    f.image(picture, slot(0, 1, "fill"))

    # The same picture at half opacity, which is the draw's own parameter.
    f.image(picture, fit_rect(WIDTH, HEIGHT, slot(1, 1, "half opacity")), opacity=0.5)

    # Rounded: a clip in the shape of the well, so the corners come off the picture.
    box = slot(2, 1, "rounded")
    f.canvas.save()
    f.clip(box, radius=12)
    scale = max(box.width / WIDTH, box.height / HEIGHT)
    f.image(
        picture,
        Rect(
            box.x + (box.width - WIDTH * scale) / 2,
            box.y + (box.height - HEIGHT * scale) / 2,
            WIDTH * scale,
            HEIGHT * scale,
        ),
    )
    f.canvas.restore()


def make_ui() -> Ui:
    return Ui(Images(), render, width=460, height=280)


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
