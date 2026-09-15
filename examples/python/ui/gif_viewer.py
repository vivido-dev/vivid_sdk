"""An animation playing at its own frame rate.

Python twin of `vivid_ui/examples/gif_viewer`. Nothing here drives the animation: the frame
being shown is a function of how long the window has been up, and the view asks the loop to
wake it exactly when the next frame is due — so a held window sleeps, and a playing one
wakes as often as the animation asks for and no more.

The Rust original carries a generated GIF through a real encoder and decoder; the binding
takes raw pixels, so this port generates the same eight frames directly and uploads each
one. The lesson — a picture advancing on the window's clock — is unchanged.
"""
from __future__ import annotations

import math

from _ui import Frame, Keys, Ui, duration
from vivid_sdk.overlay import KeyEvent, Point, Rect

#: How many frames the generated animation has, and how long each one lasts.
FRAMES = 8
FRAME_MS = 80
SIZE = 96


def walker_frame(step: int) -> bytes:
    """One frame of a dot walking around a ring: the same picture the Rust example encodes
    into a GIF, as the pixels the binding actually takes."""
    angle = step / FRAMES * math.tau
    centre = SIZE / 2.0
    radius = SIZE * 0.3
    cx = centre + math.cos(angle) * radius
    cy = centre + math.sin(angle) * radius
    pixels = bytearray()
    for y in range(SIZE):
        for x in range(SIZE):
            near = math.hypot(x - cx, y - cy) < SIZE * 0.16
            pixels += bytes((0x8E, 0xCB, 0xFF, 0xFF) if near else (0x14, 0x14, 0x1C, 0xFF))
    return bytes(pixels)


class Gif:
    playing = True


def render(state: Gif, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x101018FF)
    f.caption(
        "playing — space to hold" if state.playing else "held — space to play",
        Point(20, 20),
    )

    # Held means the first frame, exactly as a paused source in the Rust toolkit reads the
    # clock at zero. Playing means the frame the window's age says is on.
    frame = 0
    if state.playing:
        moment = f.elapsed
        frame = int(moment * 1000 / FRAME_MS) % FRAMES
        step = FRAME_MS / 1000.0
        f.request_frame_after(step - (moment % step))
    image = f.upload(f"walker-{frame}", SIZE, SIZE, walker_frame(frame))
    f.image(image, Rect(20, 46, 160, 160))

    f.label(
        f"{FRAMES} frames at {FRAME_MS}ms",
        Rect(20, 218, f.width - 40, 14),
        size=11,
        color=0x606080FF,
        vcenter=False,
    )


def make_ui() -> Ui:
    state = Gif()
    ui = Ui(state, render, width=320, height=260)

    def handle_key(event: KeyEvent) -> None:
        if event.down and not event.repeat and event.physical == Keys.SPACE:
            state.playing = not state.playing
            ui.notify()

    ui.on_key = handle_key
    return ui


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
