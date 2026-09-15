"""Easings, a spring, and a window that sleeps when nothing moves.

Python twin of `vivid_ui/examples/animation`. No timer runs here: every moving thing is a
function of how long the window has been up, and `request_frame_after` asks for the next
frame while there is still one worth drawing — so the loop wakes at the display's rate
while something is moving and not at all when nothing is.

(The Rust view also honours the environment's reduced-motion preference; the helper has no
environment plumbing, so this port always animates.)
"""
from __future__ import annotations

from typing import Callable

from _ui import (
    Animation,
    Easing,
    Frame,
    Keys,
    Running,
    Spring,
    SpringState,
    Ui,
    duration,
)
from vivid_sdk.overlay import KeyEvent, PointerEvent, Rect

TRACK = 300.0
DOT = 14.0
SWEEP = 1.4

#: The easings shown, in the order they are drawn.
EASINGS = [
    ("linear", Easing.LINEAR),
    ("ease in", Easing.EASE_IN),
    ("ease out", Easing.EASE_OUT),
    ("ease in out", Easing.EASE_IN_OUT),
]


class Animated:
    def __init__(self) -> None:
        # One animation, read by every row: the easings differ, not the clock.
        self.sweep = Running(Animation(SWEEP, repeat="ping-pong"), 0.0)
        # A spring that follows wherever it was last sent.
        self.spring = SpringState()
        self.spring_config = Spring.bouncy()


def render(state: Animated, f: Frame) -> None:
    f.box(Rect(0, 0, f.width, f.height), bg=0x101018FF)

    # The sweep reads the clock and asks for the next frame in one place, so a moving thing
    # cannot be drawn without being scheduled. It ping-pongs, so it never stops asking.
    progress = state.sweep.value(f.elapsed)
    f.request_frame_after(1.0 / 60.0)
    travel = TRACK - DOT

    y = 20.0
    for label, easing in EASINGS:
        # The same progress, shaped differently: the easings are the comparison.
        f.label(label, Rect(20, y, TRACK, 12), size=10, color=0x8080A0FF, vcenter=False)
        track = Rect(20, y + 14, TRACK, DOT)
        eased = Easing.apply(easing, progress)
        f.box(track, bg=0x1A1A24FF, radius=DOT / 2)
        f.box(
            Rect(track.x + eased * travel, track.y, DOT, DOT),
            bg=0x8ECBFFFF,
            radius=DOT / 2,
        )
        y += 12 + 2 + DOT + 10

    f.label(
        "click the track to send the spring there",
        Rect(20, y, TRACK, 12),
        size=10,
        color=0x8080A0FF,
        vcenter=False,
    )
    y += 12 + 10

    # The spring is advanced to now and asks for frames until it settles.
    state.spring.advance(state.spring_config, f.dt)
    if not state.spring.at_rest():
        f.request_frame_after(1.0 / 60.0)
    spring_track = Rect(20, y, TRACK, DOT + 6)
    f.box(spring_track, bg=0x1A1A24FF, radius=(DOT + 6) / 2)
    f.box(
        Rect(spring_track.x + state.spring.value * travel, spring_track.y + 3, DOT, DOT),
        bg=0x70D0A0FF,
        radius=DOT / 2,
    )
    f.region("spring-track", spring_track, radius=(DOT + 6) / 2, cursor="pointer",
             on_click=sent(state, f))


def sent(state: Animated, f: Frame) -> Callable[[PointerEvent], None]:
    def handler(event: PointerEvent) -> None:
        # Where the click landed as a fraction of the track — read off the frame before
        # this one, which is the frame the host hit-tested against.
        track = f.bounds_of("spring-track")
        within = 0.0
        if track is not None:
            within = (event.position.x - track.x) / max(track.width, 1.0)
        # Retargeting mid-flight keeps whatever speed it had, which is the whole reason
        # this is a spring and not a duration.
        state.spring.retarget(min(max(within, 0.0), 1.0))
        f.notify()

    return handler


def make_ui() -> Ui:
    state = Animated()
    ui = Ui(state, render, width=360, height=320)

    def handle_key(event: KeyEvent) -> None:
        if not (event.down and not event.repeat and event.physical == Keys.SPACE):
            return
        state.sweep.restart(ui.elapsed())
        ui.notify()

    ui.on_key = handle_key
    return ui


def main() -> None:
    make_ui().run(duration())


if __name__ == "__main__":
    main()
