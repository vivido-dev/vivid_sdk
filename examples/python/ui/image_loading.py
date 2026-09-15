"""A picture that arrives, and one that never will.

Python twin of `vivid_ui/examples/image_loading`. Pixels come from somewhere else — a fetch,
a file, a side channel — so the bytes are not always what they were promised to be. A
picture that cannot be decoded leaves the box it would have been drawn in, styled and empty,
and the frame keeps the rest of its content.

The Rust original generates a PNG and hands it to the toolkit's decoder; the binding takes
raw pixels, so this port generates the checkerboard directly. And since `upload_rgba` has
no decoder to refuse them, "bytes that are not a picture" cannot exist here — the port
draws the styled empty box the Rust view's failed decode leaves behind, which is the
observable lesson either way.
"""
from __future__ import annotations

from _ui import Frame, Ui, duration
from vivid_sdk.overlay import Point, Rect

#: How long the good picture takes to "arrive", so the wait is visible rather than instant.
ARRIVING = 0.6


def checkerboard(size: int, square: int) -> bytes:
    """A checkerboard as raw RGBA: the same picture the Rust example encodes as a PNG, in
    the form the binding actually takes."""
    pixels = bytearray()
    for y in range(size):
        for x in range(size):
            if ((x // max(square, 1)) + (y // max(square, 1))) % 2 == 0:
                pixels += bytes((0x30, 0x38, 0x60, 0xFF))
            else:
                pixels += bytes((0x80, 0x90, 0xE0, 0xFF))
    return bytes(pixels)


class Loading:
    arriving = ARRIVING
    elapsed = 0.0

    def has_arrived(self) -> bool:
        return self.elapsed >= self.arriving


def render(state: Loading, f: Frame) -> None:
    # The clock is the window's, so a test can stop it and watch the wait without waiting.
    state.elapsed = f.elapsed
    f.box(Rect(0, 0, f.width, f.height), bg=0x101018FF)

    def slot(x: float) -> Rect:
        return Rect(x, 36, 120, 96)

    def labelled(label: str, x: float) -> None:
        f.label(label, Rect(x, 20, 120, 12), size=10, color=0x606078FF, vcenter=False)
        f.box(slot(x), bg=0x1A1A24FF, radius=6, border=1, border_color=0x303048FF)

    # A picture that has not arrived is not an error — it is a box with nothing in it yet.
    labelled("arriving", 20)
    if state.has_arrived():
        picture = f.upload("checkerboard", 96, 96, checkerboard(96, 12))
        f.image(picture, slot(20))
    else:
        f.label("loading…", slot(20), size=11, color=0x606078FF, align="center")
        # The window is asked for the frame that will show it, at the moment it arrives.
        # There is no timer anywhere: a wait is a deadline on the window's own clock.
        f.request_frame_after(state.arriving - state.elapsed)

    # The bytes here would be an error page. The box is drawn, the picture is not, and the
    # two pictures beside it are unaffected.
    labelled("not a picture", 156)

    # The undamaged one, unaffected by its neighbour's failure.
    labelled("undamaged", 292)
    picture = f.upload("checkerboard", 96, 96, checkerboard(96, 12))
    f.image(picture, slot(292))

    f.label(
        "a picture that cannot be decoded leaves its box behind",
        Rect(20, 144, f.width - 40, 14),
        size=11,
        color=0x606078FF,
        vcenter=False,
    )


def make_ui() -> Ui:
    return Ui(Loading(), render, width=440, height=240)


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
