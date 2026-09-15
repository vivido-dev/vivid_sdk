"""A grid of pictures against a bounded upload budget.

Python twin of `vivid_ui/examples/image_gallery`. The host holds a bounded number of
uploads, and a gallery is the case where that matters: scroll far enough and the pictures on
screen are not the pictures uploaded first.

The Rust toolkit has a cache that releases whatever was drawn least recently, so the view
never thinks about it. A port has no toolkit, so the example owns that policy itself — which
is the honest version of the lesson: someone has to decide what is no longer on screen, and
here it is visibly the application. Pictures are generated rather than decoded, there being
no decoder in the standard library.
"""
from __future__ import annotations

from typing import Any, Dict, List, Tuple

from _ui import Frame, ListState, Ui, duration
from vivid_sdk.overlay import Rect, RetainedImage, WheelEvent

TILES = 60
COLUMNS = 4
ROW_HEIGHT = 86.0
VIEWPORT = 200.0
TILE = 48

#: How many uploads the gallery keeps. Smaller than the host's own ceiling, so the eviction
#: is this example's decision rather than the host's refusal.
BUDGET = 24


def gradient(tint: Tuple[int, int, int]) -> bytes:
    """A small gradient, generated so the gallery carries its own pictures."""
    pixels = bytearray()
    for y in range(TILE):
        for x in range(TILE):
            across = x * 255 // TILE
            down = y * 255 // TILE
            pixels.extend(
                (
                    across * tint[0] // 255,
                    down * tint[1] // 255,
                    (across + down) // 2 * tint[2] // 255,
                    0xFF,
                )
            )
    return bytes(pixels)


def tint_of(index: int) -> Tuple[int, int, int]:
    return ((60 + index * 3) % 256, (120 + index * 5) % 256, (200 - index * 2) % 256)


class Gallery:
    def __init__(self) -> None:
        rows = (TILES + COLUMNS - 1) // COLUMNS
        self.list = ListState(rows, ROW_HEIGHT)
        #: The uploads this gallery is holding, oldest first.
        self.held: Dict[int, RetainedImage] = {}
        self.order: List[int] = []

    def picture(self, f: Frame, index: int) -> RetainedImage:
        """The picture for a tile, uploading it if this gallery is not already holding it,
        and letting the oldest one go when the budget is spent."""
        image = self.held.get(index)
        if image is None:
            image = f.upload(f"tile-{index}", TILE, TILE, gradient(tint_of(index)))
            self.held[index] = image
            self.order.append(index)
            while len(self.order) > BUDGET:
                oldest = self.order.pop(0)
                f.ui.release_image(self.held.pop(oldest))
        else:
            # Drawing is what keeps a picture: touching it moves it off the chopping block.
            self.order.remove(index)
            self.order.append(index)
        return image


def render(state: Gallery, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x101018FF)
    f.label(
        f"{TILES} pictures, a row at a time · {len(state.order)} of {BUDGET} uploads held",
        Rect(16, 16, f.width - 32, 14),
        size=12,
        color=0x8080A0FF,
        vcenter=False,
    )

    box = Rect(16, 38, f.width - 32, VIEWPORT)

    def scrolled(event: Any) -> None:
        if isinstance(event, WheelEvent):
            state.list.scroll_by(event.dy, box.height)
            f.notify()

    f.region("gallery", box, radius=6, on_wheel=scrolled)
    f.canvas.save()
    f.clip(box, radius=6)
    first, last = state.list.visible_range(box.height)
    for row in range(first, last):
        top = box.y + state.list.row_top(row)
        for column in range(COLUMNS):
            index = row * COLUMNS + column
            if index >= TILES:
                break
            tile = Rect(box.x + 8 + column * (72 + 8), top + 7, 72, 72)
            f.image(state.picture(f, index), tile)
    f.canvas.restore()


def make_ui() -> Ui:
    return Ui(Gallery(), render, width=400, height=260)


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
